=========================================================================

LOBSTER | academic data.

Limit Order Book System - The Efficient Reconstructor

http://LOBSTER.wiwi.hu-berlin.de

					This Version: 	01 Sept 2013
=========================================================================

Sample Files Readme

=========================================================================
										

Output Structure:
---------------

LOBSTER generates a 'message' and an 'orderbook' file for each active 
trading day of a selected ticker. The 'orderbook' file contains the 
evolution of the limit order book up to the requested number of levels. 
The 'message' file contains indicators for the type of event causing 
an update of the limit order book in the requested price range. All 
events are timestamped to seconds after midnight, with decimal 
precision of at least milliseconds and up to nanoseconds depending 
on the requested period. 


	Message File:		(Matrix of size: (Nx6))
	-------------	
			
	Name: 	TICKER_Year-Month-Day_StartTime_EndTime_message_LEVEL.csv 	
		
		StartTime and EndTime give the theoretical beginning 
		and end time of the output file in milliseconds after 		
		mid night. LEVEL refers to the number of levels of the 
		requested limit order book.


	Columns:
	
	    1.) Time: 		
				Seconds after midnight with decimal 
				precision of at least milliseconds 
				and up to nanoseconds depending on 
				the requested period
	    2.) Type:
				1: Submission of a new limit order
				2: Cancellation (Partial deletion 
				   of a limit order)
				3: Deletion (Total deletion of a limit order)
				4: Execution of a visible limit order			   	 
				5: Execution of a hidden limit order
				7: Trading halt indicator 				   
				   (Detailed information below)
	    3.) Order ID: 	
				Unique order reference number 
				(Assigned in order flow)
	    4.) Size: 		
				Number of shares
	    5.) Price: 		
				Dollar price times 10000 
				(i.e., A stock price of $91.14 is given 
				by 911400)
	    6.) Direction:
				-1: Sell limit order
				1: Buy limit order
				
				Note: 
				Execution of a sell (buy) limit
				order corresponds to a buyer (seller) 
				initiated trade, i.e. Buy (Sell) trade.
										
						
	Orderbook File:		(Matrix of size: (Nx(4xNumberOfLevels)))
	---------------
	
	Name: 	TICKER_Year-Month-Day_StartTime_EndTime_orderbook_LEVEL.csv
	
	Columns:
	
 	    1.) Ask Price 1: 	Level 1 Ask Price 	(Best Ask)
	    2.) Ask Size 1: 	Level 1 Ask Volume 	(Best Ask Volume)
	    3.) Bid Price 1: 	Level 1 Bid Price 	(Best Bid)
	    4.) Bid Size 1: 	Level 1 Bid Volume 	(Best Bid Volume)
	    5.) Ask Price 2: 	Level 2 Ask Price 	(2nd Best Ask)
	    ...
	
	Notes: 	 
	------
	
		- Levels:
		
		The term level refers to occupied price levels. This implies 
		that the difference between two levels in the LOBSTER output 
		is not necessarily the minimum ticks size.

		- Unoccupied Price Levels:
	
		When the selected number of levels exceeds the number of levels 
		available the empty order book positions are filled with dummy 
		information to guarantee a symmetric output. The extra bid 
		and/or ask prices are set to -9999999999 and 9999999999, 
		respectively. The Corresponding volumes are set to 0. 
		
		- Trading Halts:
		
		When trading halts, a message of type '7' is written into the 
		'message' file. The corresponding price and trade direction 
		are set to '-1' and all other properties are set to '0'. 
		Should the resume of quoting be indicated by an additional 
		message in NASDAQ's Historical TotalView-ITCH files, another 
		message of type '7' with price '0' is added to the 'message' 
		file. Again, the trade direction is set to '-1' and all other 
		fields are set to '0'. 
		When trading resumes a message of type '7' and 
		price '1' (Trade direction '-1' and all other 
		entries '0') is written to the 'message' file. For messages 
		of type '7', the corresponding order book rows contain a 
		duplication of the preceding order book state. The reason 
		for the trading halt is not included in the output.
						
			Example: Stylized trading halt messages in 'message' file.				
		
			Halt: 				36023	| 7 | 0 | 0 | -1 | -1
											...
			Quoting: 			36323 	| 7 | 0 | 0 | 0  | -1
											...
			Resume Trading:		36723   | 7 | 0 | 0 | 1  | -1
											...

			The vertical bars indicate the different columns in the  
			message file.
			
=========================================================================

Any questions? Just contact us at http://LOBSTER.wiwi.hu-berlin.de

=========================================================================
